//! Error types for peer identity operations.

/// Errors from multiaddr serialization/deserialization.
#[derive(Debug, thiserror::Error)]
pub enum MultiAddrError {
    #[error("empty byte slice")]
    EmptyData,
    #[error("failed to read varint: {0}")]
    VarintError(#[from] std::io::Error),
    #[error("inconsistent data: expected {expected} bytes, got {actual}")]
    InconsistentLength { expected: u64, actual: usize },
    #[error("failed to parse multiaddr: {0}")]
    InvalidMultiaddr(#[from] libp2p::multiaddr::Error),
}

/// Errors from [`SwarmPeer`](crate::SwarmPeer) construction.
#[derive(Debug, thiserror::Error)]
pub enum SwarmPeerError {
    #[error("invalid signature: {0}")]
    InvalidSignature(#[from] alloy_primitives::SignatureError),
    #[error("signer error: {0}")]
    SignerError(#[from] alloy_signer::Error),
    #[error("computed overlay does not match claimed overlay")]
    InvalidOverlay,
    #[error("at least one multiaddr is required")]
    NoMultiaddrs,
    #[error("invalid multiaddr encoding: {0}")]
    InvalidMultiaddr(#[from] MultiAddrError),
}
