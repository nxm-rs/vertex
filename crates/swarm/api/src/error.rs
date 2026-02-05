//! Error types for Swarm API operations.

use libp2p::multiaddr;
use nectar_primitives::ChunkAddress;
use std::string::String;

/// Error type for Swarm API operations.
#[derive(Debug, thiserror::Error)]
pub enum SwarmError {
    /// Chunk not found in the network.
    #[error("chunk not found: {address}")]
    ChunkNotFound {
        /// The address of the chunk that wasn't found.
        address: ChunkAddress,
    },

    /// Storage operation failed.
    #[error("storage error: {message}")]
    Storage {
        /// Description of the storage failure.
        message: String,
    },

    /// Network operation failed.
    #[error("network error: {message}")]
    Network {
        /// Description of the network failure.
        message: String,
    },

    /// Peer disconnected or unavailable.
    #[error("peer unavailable: {reason}")]
    PeerUnavailable {
        /// Description of why the peer is unavailable.
        reason: String,
    },

    /// Bandwidth limit exceeded (peer owes too much).
    #[error("bandwidth limit exceeded: balance {balance} > threshold {threshold}")]
    BandwidthLimitExceeded {
        /// Current balance with the peer.
        balance: i64,
        /// The threshold that was exceeded.
        threshold: i64,
    },

    /// Payment required but not provided or invalid.
    #[error("payment required: {reason}")]
    PaymentRequired {
        /// Description of the payment requirement.
        reason: String,
    },

    /// Invalid chunk data.
    #[error("invalid chunk: {reason}")]
    InvalidChunk {
        /// Description of why the chunk is invalid.
        reason: String,
    },

    /// Accounting operation failed.
    #[error("accounting error: {0}")]
    Accounting(String),
}

/// Result type for Swarm API operations.
pub type SwarmResult<T> = core::result::Result<T, SwarmError>;

/// Error type for configuration validation.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Invalid listen address.
    #[error("invalid listen address '{addr}': {source}")]
    InvalidListenAddr {
        /// The invalid address string.
        addr: String,
        /// The parse error.
        #[source]
        source: multiaddr::Error,
    },

    /// Invalid bootnode address.
    #[error("invalid bootnode address '{addr}': {source}")]
    InvalidBootnode {
        /// The invalid address string.
        addr: String,
        /// The parse error.
        #[source]
        source: multiaddr::Error,
    },

    /// Invalid NAT address.
    #[error("invalid NAT address '{addr}': {source}")]
    InvalidNatAddr {
        /// The invalid address string.
        addr: String,
        /// The parse error.
        #[source]
        source: multiaddr::Error,
    },

    /// Invalid trusted peer address.
    #[error("invalid trusted peer address '{addr}': {source}")]
    InvalidTrustedPeer {
        /// The invalid address string.
        addr: String,
        /// The parse error.
        #[source]
        source: multiaddr::Error,
    },
}

/// Result type for configuration operations.
pub type ConfigResult<T> = core::result::Result<T, ConfigError>;
