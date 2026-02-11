//! Error types for pseudosettle protocol.

use std::convert::Infallible;

use strum::IntoStaticStr;

/// Pseudosettle protocol errors.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum PseudosettleError {
    /// Connection closed before operation completed.
    #[error("connection closed")]
    ConnectionClosed,

    /// Invalid timestamp in acknowledgment.
    #[error("invalid timestamp: {0}")]
    #[strum(serialize = "invalid_timestamp")]
    InvalidTimestamp(String),

    /// Protobuf encoding/decoding error.
    #[error("protobuf error: {0}")]
    #[strum(serialize = "protobuf_error")]
    Protobuf(#[from] quick_protobuf_codec::Error),

    /// I/O error during stream operations.
    #[error("io error: {0}")]
    #[strum(serialize = "io_error")]
    Io(#[from] std::io::Error),
}

impl From<Infallible> for PseudosettleError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}
