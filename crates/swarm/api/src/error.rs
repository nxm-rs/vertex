//! Error types for Swarm API operations.

use libp2p::multiaddr;
use nectar_primitives::ChunkAddress;
use std::string::String;
use vertex_swarm_primitives::OverlayAddress;

/// Error type for Swarm API operations.
#[derive(Debug, thiserror::Error)]
pub enum SwarmError {
    /// Chunk not found in the network.
    #[error("chunk not found: {address}")]
    ChunkNotFound {
        /// The address of the chunk that wasn't found.
        address: ChunkAddress,
    },

    /// No storer found for the chunk in proximity range.
    #[error("no storer found for chunk: {chunk_address}")]
    NoStorer {
        /// The chunk address that couldn't be stored.
        chunk_address: ChunkAddress,
    },

    /// Invalid postage stamp signature.
    #[error("invalid stamp signature for {chunk_address}: {reason}")]
    InvalidSignature {
        /// The chunk whose stamp failed validation.
        chunk_address: ChunkAddress,
        /// Description of the signature validation failure.
        reason: String,
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
    #[error("peer unavailable{}: {reason}", peer.map(|p| format!(": {}", p)).unwrap_or_default())]
    PeerUnavailable {
        /// The peer that became unavailable, if known.
        peer: Option<OverlayAddress>,
        /// Description of why the peer is unavailable.
        reason: String,
    },

    /// Bandwidth limit exceeded (peer owes too much).
    #[error("bandwidth limit exceeded for {peer}: balance {balance} > threshold {threshold}")]
    BandwidthLimitExceeded {
        /// The peer whose bandwidth limit was exceeded.
        peer: OverlayAddress,
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
    #[error("invalid chunk{}: {reason}", address.map(|a| format!(": {}", a)).unwrap_or_default())]
    InvalidChunk {
        /// The chunk address, if known.
        address: Option<ChunkAddress>,
        /// Description of why the chunk is invalid.
        reason: String,
    },

    /// Accounting operation failed.
    #[error("accounting error: {message}")]
    Accounting {
        /// Description of the accounting failure.
        message: String,
    },

    /// Internal error.
    #[error("internal error: {message}")]
    Internal {
        /// Description of the internal failure.
        message: String,
    },
}

impl SwarmError {
    /// Whether this error represents a transient failure that may succeed on retry.
    ///
    /// Retryable errors include network issues, peer unavailability, and accounting
    /// failures. Non-retryable errors include invalid data, missing chunks, and
    /// configuration issues.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Network { .. }
                | Self::PeerUnavailable { .. }
                | Self::Accounting { .. }
                | Self::NoStorer { .. }
        )
    }

    /// Whether this error indicates the requested data does not exist.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::ChunkNotFound { .. })
    }

    /// Whether this error indicates invalid input data.
    pub fn is_invalid_input(&self) -> bool {
        matches!(
            self,
            Self::InvalidChunk { .. } | Self::InvalidSignature { .. }
        )
    }
}

/// Result type for Swarm API operations.
pub type SwarmResult<T> = core::result::Result<T, SwarmError>;

/// Kind of address that failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigAddressKind {
    /// Listen address for P2P connections.
    ListenAddr,
    /// Bootnode address for initial peer discovery.
    Bootnode,
    /// NAT address for external advertisement.
    NatAddr,
    /// Trusted peer address.
    TrustedPeer,
}

impl core::fmt::Display for ConfigAddressKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ListenAddr => write!(f, "listen address"),
            Self::Bootnode => write!(f, "bootnode address"),
            Self::NatAddr => write!(f, "NAT address"),
            Self::TrustedPeer => write!(f, "trusted peer address"),
        }
    }
}

/// Error type for configuration validation.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Invalid multiaddress.
    #[error("invalid {kind} '{addr}': {source}")]
    InvalidAddress {
        /// The type of address that failed validation.
        kind: ConfigAddressKind,
        /// The invalid address string.
        addr: String,
        /// The parse error.
        #[source]
        source: multiaddr::Error,
    },
}

/// Result type for configuration operations.
pub type ConfigResult<T> = core::result::Result<T, ConfigError>;
