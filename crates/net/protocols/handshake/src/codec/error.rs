//! Error types for handshake protocol codec.

use libp2p::multiaddr;
use vertex_swarm_peer::SwarmPeerError;
use vertex_net_codec::ProtocolCodecError;

/// Domain-specific errors for handshake protocol.
#[derive(Debug, thiserror::Error)]
pub enum HandshakeCodecDomainError {
    /// Network ID mismatch between peers.
    #[error("network ID mismatch")]
    NetworkIdMismatch,

    /// Required field missing from message.
    #[error("missing field: {0}")]
    MissingField(&'static str),

    /// Field exceeds maximum allowed length.
    #[error("{0} exceeds max length {1}, got {2}")]
    FieldLengthExceeded(&'static str, usize, usize),

    /// Invalid data conversion (e.g., slice to array).
    #[error("invalid data: {0}")]
    InvalidData(#[from] std::array::TryFromSliceError),

    /// Invalid multiaddr encoding.
    #[error("invalid multiaddr: {0}")]
    InvalidMultiaddr(#[from] multiaddr::Error),

    /// Invalid cryptographic signature.
    #[error("invalid signature: {0}")]
    InvalidSignature(#[from] alloy_primitives::SignatureError),

    /// Invalid peer identity.
    #[error("invalid peer: {0}")]
    InvalidPeer(#[from] SwarmPeerError),
}

/// Error type for handshake codec operations.
///
/// Uses the generic `ProtocolCodecError` with handshake-specific domain errors.
pub type CodecError = ProtocolCodecError<HandshakeCodecDomainError>;
