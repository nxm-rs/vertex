//! Error types for pushsync protocol.

use std::convert::Infallible;

use strum::IntoStaticStr;

/// Pushsync protocol errors.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum PushsyncError {
    /// Connection closed before operation completed.
    #[error("connection closed")]
    ConnectionClosed,

    /// Invalid chunk address length.
    #[error("invalid chunk address length: expected 32, got {0}")]
    #[strum(serialize = "invalid_address_length")]
    InvalidAddressLength(usize),

    /// Invalid chunk address encoding.
    #[error("invalid chunk address: {0}")]
    #[strum(serialize = "invalid_address")]
    InvalidAddress(String),

    /// Storage radius exceeds maximum (255).
    #[error("invalid storage radius: {0} exceeds u8 range")]
    #[strum(serialize = "invalid_storage_radius")]
    InvalidStorageRadius(u32),

    /// Protobuf encoding/decoding error.
    #[error("protobuf error: {0}")]
    #[strum(serialize = "protobuf_error")]
    Protobuf(#[from] quick_protobuf_codec::Error),

    /// I/O error during stream operations.
    #[error("io error: {0}")]
    #[strum(serialize = "io_error")]
    Io(#[from] std::io::Error),
}

impl From<Infallible> for PushsyncError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}
