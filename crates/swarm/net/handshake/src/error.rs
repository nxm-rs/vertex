//! Error types for handshake protocol.

use std::convert::Infallible;

use strum::IntoStaticStr;
use vertex_swarm_peer::{MultiAddrError, SwarmPeerError};

/// Handshake protocol errors.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum HandshakeError {
    /// Handshake timeout.
    #[error("timeout")]
    Timeout,

    /// Connection closed before handshake completed.
    #[error("connection closed")]
    ConnectionClosed,

    /// Network ID mismatch between peers.
    #[error("network ID mismatch")]
    NetworkIdMismatch,

    /// Required field missing from message.
    #[error("missing field: {0}")]
    #[strum(serialize = "missing_field")]
    MissingField(&'static str),

    /// Field exceeds maximum allowed length.
    #[error("{field} exceeds max length {max}, got {actual}")]
    #[strum(serialize = "field_too_long")]
    FieldTooLong {
        field: &'static str,
        max: usize,
        actual: usize,
    },

    /// Invalid data conversion (e.g., slice to fixed-size array).
    #[error("invalid data: {0}")]
    #[strum(serialize = "invalid_data")]
    InvalidData(#[from] std::array::TryFromSliceError),

    /// Invalid multiaddr encoding.
    #[error("invalid multiaddr: {0}")]
    #[strum(serialize = "invalid_multiaddr")]
    InvalidMultiaddr(#[from] MultiAddrError),

    /// Invalid signature bytes (wrong length or format).
    #[error("invalid signature: {0}")]
    #[strum(serialize = "invalid_signature")]
    InvalidSignature(#[from] alloy_primitives::SignatureError),

    /// Invalid peer identity (signature verification or overlay mismatch).
    #[error("invalid peer: {0}")]
    #[strum(serialize = "invalid_peer")]
    InvalidPeer(#[from] SwarmPeerError),

    /// Invalid overlay address.
    #[error("invalid overlay")]
    InvalidOverlay,

    /// Observed address has wrong or missing peer ID.
    #[error("invalid observed address")]
    InvalidObservedAddress,

    /// Protobuf encoding/decoding error.
    #[error("protobuf error: {0}")]
    #[strum(serialize = "protobuf_error")]
    Protobuf(#[from] quick_protobuf_codec::Error),

    /// I/O error during stream operations.
    #[error("io error: {0}")]
    #[strum(serialize = "io_error")]
    Io(#[from] std::io::Error),

    /// Stream upgrade failed at libp2p layer.
    #[error("upgrade error: {0}")]
    #[strum(serialize = "upgrade_error")]
    UpgradeError(String),
}

impl From<Infallible> for HandshakeError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}

impl From<vertex_net_codec::StreamClosed> for HandshakeError {
    fn from(_: vertex_net_codec::StreamClosed) -> Self {
        Self::ConnectionClosed
    }
}
