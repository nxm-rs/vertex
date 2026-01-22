//! Error types for the headers protocol.

use crate::codec::CodecError;

/// Error during headers exchange.
#[derive(Debug, thiserror::Error)]
pub enum HeadersError {
    #[error("Codec error: {0}")]
    Codec(#[from] CodecError),
    #[error("Connection closed")]
    ConnectionClosed,
}

/// Error from a headered protocol upgrade.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("Headers error: {0}")]
    Headers(#[from] HeadersError),
    #[error("Protocol error: {0}")]
    Protocol(Box<dyn std::error::Error + Send + Sync>),
}
