//! Error types for pricing protocol.

use std::convert::Infallible;

use strum::IntoStaticStr;

/// Pricing protocol errors.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum PricingError {
    /// Connection closed before operation completed.
    #[error("connection closed")]
    ConnectionClosed,

    /// Protobuf encoding/decoding error.
    #[error("protobuf error: {0}")]
    #[strum(serialize = "protobuf_error")]
    Protobuf(#[from] quick_protobuf_codec::Error),

    /// I/O error during stream operations.
    #[error("io error: {0}")]
    #[strum(serialize = "io_error")]
    Io(#[from] std::io::Error),
}

impl From<Infallible> for PricingError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}
