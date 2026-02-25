//! Topology commands and events.

use std::time::Duration;

use libp2p::{Multiaddr, PeerId};
use vertex_swarm_primitives::OverlayAddress;

// Re-export from the peer registry crate
pub use vertex_swarm_peer_registry::{ConnectionDirection, DialReason};

/// Reason for peer disconnection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum DisconnectReason {
    /// Remote peer initiated the disconnect.
    Remote,
    /// Local node closed the connection (e.g., bin pruning).
    LocalClose,
    /// Connection timed out or network error.
    ConnectionError,
    /// Graceful shutdown in progress.
    Shutdown,
    /// Evicted because bin exceeded target after depth change.
    BinTrimmed,
    /// Unknown or unclassified reason.
    Unknown,
}

/// Dial failure reasons for structured error handling.
#[derive(Debug, Clone, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum DialError {
    /// Dial timed out waiting for connection.
    Timeout,
    /// Handshake protocol failed.
    HandshakeFailed(String),
    /// Connection actively refused by peer.
    ConnectionRefused,
    /// No route to peer address.
    NoRoute,
    /// Address unreachable (network layer).
    Unreachable,
    /// Protocol negotiation failed (no common protocols).
    NegotiationFailed,
    /// Stale connection attempt cleaned up.
    Stale,
    /// Other error with description.
    Other(String),
}

impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DialError::Timeout => write!(f, "dial timed out"),
            DialError::HandshakeFailed(msg) => write!(f, "handshake failed: {}", msg),
            DialError::ConnectionRefused => write!(f, "connection refused"),
            DialError::NoRoute => write!(f, "no route to host"),
            DialError::Unreachable => write!(f, "address unreachable"),
            DialError::NegotiationFailed => write!(f, "protocol negotiation failed"),
            DialError::Stale => write!(f, "stale connection attempt"),
            DialError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

/// Reason for rejecting a peer connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum RejectionReason {
    /// Kademlia bin is saturated.
    BinSaturated,
    /// Peer is banned.
    Banned,
    /// Duplicate connection from same peer.
    DuplicateConnection,
    /// Handshake validation failed.
    HandshakeFailed,
}

/// Events emitted by TopologyService for external consumers.
#[derive(Debug, Clone)]
pub enum TopologyEvent {
    /// Peer completed handshake and is ready for protocol use.
    PeerReady {
        overlay: OverlayAddress,
        peer_id: PeerId,
        /// True if peer is a storer node (full storage commitment).
        storer: bool,
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
        /// Whether the disconnected peer was a storer node.
        storer: bool,
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
}
