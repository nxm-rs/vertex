//! Peer manager events.
//!
//! All events use OverlayAddress only - no PeerId leakage.

use libp2p::Multiaddr;
use vertex_primitives::OverlayAddress;

/// Events emitted by the peer manager.
///
/// All events use OverlayAddress (Swarm layer) rather than PeerId (libp2p layer).
/// This maintains the abstraction boundary.
#[derive(Debug, Clone)]
pub enum PeerManagerEvent {
    /// A peer has connected and completed handshake.
    PeerConnected {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// Whether this is a full node.
        is_full_node: bool,
    },

    /// A peer has disconnected.
    PeerDisconnected {
        /// The peer's overlay address.
        overlay: OverlayAddress,
    },

    /// New peers were discovered (e.g., via hive protocol).
    PeersDiscovered {
        /// The overlay address of the peer that sent us the peer list.
        from: OverlayAddress,
        /// The discovered peers with their known underlay addresses.
        peers: Vec<(OverlayAddress, Vec<Multiaddr>)>,
    },

    /// A connection attempt failed.
    ConnectionFailed {
        /// The overlay address we tried to connect to.
        overlay: OverlayAddress,
        /// Error description.
        error: String,
    },
}
