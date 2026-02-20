//! Error types for swap protocol.

use std::convert::Infallible;

use strum::IntoStaticStr;

/// SWAP protocol errors.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum SwapError {
    /// Connection closed before operation completed.
    #[error("connection closed")]
    ConnectionClosed,

    /// Missing required header.
    #[error("missing header: {0}")]
    #[strum(serialize = "missing_header")]
    MissingHeader(&'static str),

    /// Invalid beneficiary address length.
    #[error("invalid beneficiary length: expected 20, got {0}")]
    #[strum(serialize = "invalid_beneficiary")]
    InvalidBeneficiaryLength(usize),

    /// JSON serialization/deserialization error.
    #[error("json error: {0}")]
    #[strum(serialize = "json_error")]
    Json(String),

    /// Protobuf encoding/decoding error.
    #[error("protobuf error: {0}")]
    #[strum(serialize = "protobuf_error")]
    Protobuf(#[from] quick_protobuf_codec::Error),

    /// I/O error during stream operations.
    #[error("io error: {0}")]
    #[strum(serialize = "io_error")]
    Io(#[from] std::io::Error),
}

impl From<serde_json::Error> for SwapError {
    fn from(error: serde_json::Error) -> Self {
        SwapError::Json(error.to_string())
    }
}

impl From<Infallible> for SwapError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}
