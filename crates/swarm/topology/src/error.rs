//! Topology error and reason types.

use libp2p::Multiaddr;
use vertex_swarm_primitives::OverlayAddress;

/// Reason for peer disconnection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum DisconnectReason {
    /// Local node closed the connection (e.g., bin pruning).
    LocalClose,
    /// Connection timed out or network error.
    ConnectionError,
    /// Evicted because bin exceeded target after depth change.
    BinTrimmed,
}

/// Dial failure reasons for structured error handling.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum DialError {
    /// Dial timed out waiting for connection.
    #[error("dial timed out")]
    Timeout,
    /// Handshake protocol failed.
    #[error("handshake failed: {0}")]
    HandshakeFailed(String),
    /// Connection actively refused by peer.
    #[error("connection refused")]
    ConnectionRefused,
    /// No route to peer address.
    #[error("no route to host")]
    NoRoute,
    /// Address unreachable (network layer).
    #[error("address unreachable")]
    Unreachable,
    /// Protocol negotiation failed (no common protocols).
    #[error("protocol negotiation failed")]
    NegotiationFailed,
    /// Stale connection attempt cleaned up.
    #[error("stale connection attempt")]
    Stale,
    /// Other error with description.
    #[error("{0}")]
    Other(String),
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

/// Errors that can occur in topology operations.
#[derive(Debug, thiserror::Error)]
pub enum TopologyError {
    /// Failed to parse a multiaddr string.
    #[error("invalid multiaddr '{addr}': {reason}")]
    InvalidMultiaddr { addr: String, reason: String },

    /// Failed to resolve a dnsaddr multiaddr.
    #[error("failed to resolve dnsaddr {addr}: {reason}")]
    DnsResolutionFailed { addr: Multiaddr, reason: String },

    /// No bootnodes available or all connection attempts failed.
    #[error("failed to connect to any bootnode after {attempts} attempts")]
    BootnodeConnectionFailed { attempts: usize },

    /// Peer not found in the routing table.
    #[error("peer not found: {overlay}")]
    PeerNotFound { overlay: OverlayAddress },

    /// Routing table is full for the given bin.
    #[error("bin {bin} is saturated, cannot add peer")]
    BinSaturated { bin: u8 },

    /// Connection error.
    #[error("connection error: {0}")]
    Connection(String),

    /// The operation timed out.
    #[error("operation timed out")]
    Timeout,

    /// The topology service has shut down.
    #[error("topology service has shut down")]
    ServiceShutdown,

    /// Failed to spawn the gossip verifier task.
    #[error("failed to spawn gossip verifier: {0}")]
    VerifierSpawn(String),
}

/// Result type for topology operations.
pub type TopologyResult<T> = Result<T, TopologyError>;
