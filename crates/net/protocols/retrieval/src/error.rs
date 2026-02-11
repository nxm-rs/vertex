//! Error types for retrieval protocol.

use std::convert::Infallible;

use strum::IntoStaticStr;

/// Retrieval protocol errors.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum RetrievalError {
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

    /// Protobuf encoding/decoding error.
    #[error("protobuf error: {0}")]
    #[strum(serialize = "protobuf_error")]
    Protobuf(#[from] quick_protobuf_codec::Error),

    /// I/O error during stream operations.
    #[error("io error: {0}")]
    #[strum(serialize = "io_error")]
    Io(#[from] std::io::Error),
}

impl From<Infallible> for RetrievalError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}
