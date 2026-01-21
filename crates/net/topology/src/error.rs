//! Topology error types.

use libp2p::Multiaddr;
use vertex_primitives::OverlayAddress;

/// Errors that can occur in topology operations.
#[derive(Debug, thiserror::Error)]
pub enum TopologyError {
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
}

/// Result type for topology operations.
pub type TopologyResult<T> = Result<T, TopologyError>;
