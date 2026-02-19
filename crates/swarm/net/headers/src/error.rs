//! Error types for the headers protocol.

use std::convert::Infallible;

use strum::IntoStaticStr;

/// Error during headers exchange.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum HeadersError {
    /// Connection closed before headers exchange completed.
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

impl From<Infallible> for HeadersError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}

/// Error from a headered protocol upgrade.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// Headers exchange failed.
    #[error("headers error: {0}")]
    Headers(#[from] HeadersError),

    /// Inner protocol error.
    #[error("protocol error: {0}")]
    Protocol(Box<dyn std::error::Error + Send + Sync>),
}
