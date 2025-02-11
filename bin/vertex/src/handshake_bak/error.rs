// error.rs
use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HandshakeError {
    #[error("Network ID mismatch")]
    NetworkIdMismatch,

    #[error("Invalid message: {0}")]
    InvalidMessage(String),

    #[error("Protocol negotiation failed: {0}")]
    ProtocolNegotiation(String),

    #[error("Handshake timeout")]
    Timeout,

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Protobuf error: {0}")]
    Protobuf(#[from] quick_protobuf::Error),
}
