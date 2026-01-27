//! Error types for Swarm API operations.
//!
//! This module defines domain-specific errors for Swarm network operations.
//! Each error variant contains typed data (not strings) for better
//! programmatic handling.

use std::string::String;
use vertex_primitives::ChunkAddress;

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
}

/// Result type for Swarm API operations.
pub type SwarmResult<T> = core::result::Result<T, SwarmError>;
