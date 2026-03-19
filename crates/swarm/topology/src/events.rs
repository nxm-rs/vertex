//! Topology commands and events.

use std::time::Duration;

use libp2p::{Multiaddr, PeerId};
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

pub use vertex_net_peer_registry::ConnectionDirection;

pub(crate) use crate::error::{DialError, DisconnectReason, RejectionReason};

/// Reason for initiating a dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum DialReason {
    /// Peer discovered via Hive protocol (already verified or from peer store).
    Discovery,
    /// Connecting to a bootnode.
    Bootnode,
    /// Connecting to a trusted peer.
    Trusted,
    /// User-initiated dial command.
    Command,
}

/// Events emitted by TopologyService for external consumers.
#[derive(Debug, Clone)]
pub enum TopologyEvent {
    /// Peer completed handshake and is ready for protocol use.
    PeerReady {
        overlay: OverlayAddress,
        peer_id: PeerId,
        /// Node type (Bootnode, Client, or Storer).
        node_type: SwarmNodeType,
        /// Whether we dialed or they dialed us.
        direction: ConnectionDirection,
    },
    /// Connection was rejected (bin saturated, duplicate, etc.).
    PeerRejected {
        overlay: OverlayAddress,
        peer_id: PeerId,
        reason: RejectionReason,
        /// Whether we dialed or they dialed us.
        direction: ConnectionDirection,
    },
    /// All connections to peer closed.
    PeerDisconnected {
        overlay: OverlayAddress,
        reason: DisconnectReason,
        /// How long the peer was connected before disconnecting.
        connection_duration: Option<Duration>,
        /// Node type of the disconnected peer.
        node_type: SwarmNodeType,
    },
    /// Neighborhood depth changed.
    DepthChanged { old_depth: u8, new_depth: u8 },
    /// Dial attempt failed (all addresses exhausted).
    DialFailed {
        /// Overlay address if known.
        overlay: Option<OverlayAddress>,
        /// All addresses that were attempted.
        addrs: Vec<Multiaddr>,
        /// Typed error reason.
        error: DialError,
        /// Duration of the entire dial attempt.
        dial_duration: Option<Duration>,
        /// Dial reason.
        reason: Option<DialReason>,
    },
    /// Ping completed with RTT measurement.
    PingCompleted {
        overlay: OverlayAddress,
        rtt: Duration,
    },
}

/// Commands for the topology behaviour.
#[derive(Debug, Clone)]
pub enum TopologyCommand {
    /// Connect to bootnodes and trusted peers.
    ConnectBootnodes,
    /// Dial a peer by multiaddr.
    Dial(Multiaddr),
    /// Close all connections to a peer.
    CloseConnection(OverlayAddress),
    /// Ban a peer and remove from routing.
    BanPeer {
        overlay: OverlayAddress,
        reason: Option<String>,
    },
    /// Flush known peers to persistent storage.
    SavePeers,
}
