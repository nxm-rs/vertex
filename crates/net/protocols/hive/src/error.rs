//! Error types for hive protocol.

use std::convert::Infallible;

use strum::IntoStaticStr;

/// Hive protocol errors.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum HiveError {
    /// Connection closed before message was received.
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

impl HiveError {
    /// Get a label for metrics.
    pub fn label(&self) -> &'static str {
        self.into()
    }
}

impl From<Infallible> for HiveError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}
