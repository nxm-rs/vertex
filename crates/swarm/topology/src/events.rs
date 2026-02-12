//! Topology commands and events.

use std::time::Duration;

use libp2p::{Multiaddr, PeerId};
use vertex_swarm_primitives::OverlayAddress;

// Re-export from the peer registry crate
pub use vertex_swarm_peer_registry::{ConnectionDirection, DialReason};

/// Reason for peer disconnection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    /// Remote peer initiated the disconnect.
    Remote,
    /// Local node closed the connection (e.g., bin pruning).
    LocalClose,
    /// Connection timed out or network error.
    ConnectionError,
    /// Graceful shutdown in progress.
    Shutdown,
    /// Unknown or unclassified reason.
    Unknown,
}

impl DisconnectReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            DisconnectReason::Remote => "remote",
            DisconnectReason::LocalClose => "local_close",
            DisconnectReason::ConnectionError => "connection_error",
            DisconnectReason::Shutdown => "shutdown",
            DisconnectReason::Unknown => "unknown",
        }
    }
}

/// Reason for rejecting a peer connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl RejectionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            RejectionReason::BinSaturated => "bin_saturated",
            RejectionReason::Banned => "banned",
            RejectionReason::DuplicateConnection => "duplicate_connection",
            RejectionReason::HandshakeFailed => "handshake_failed",
        }
    }
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
        /// Duration from connection establishment to handshake completion.
        handshake_duration: Duration,
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
    },
    /// Neighborhood depth changed.
    DepthChanged { old_depth: u8, new_depth: u8 },
    /// Dial attempt failed (all addresses exhausted).
    DialFailed {
        /// Overlay address if known.
        overlay: Option<OverlayAddress>,
        /// All addresses that were attempted.
        addrs: Vec<Multiaddr>,
        /// Error description.
        error: String,
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
