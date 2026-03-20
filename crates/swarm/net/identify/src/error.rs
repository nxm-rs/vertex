//! Error types for the identify protocol.

use std::io;

use libp2p::core::multiaddr;
use libp2p::identity;

/// Errors during identify protocol upgrade.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum UpgradeError {
    #[error(transparent)]
    Codec(#[from] quick_protobuf_codec::Error),
    #[error("I/O interaction failed")]
    Io(#[from] io::Error),
    #[error("Stream closed")]
    StreamClosed,
    #[error("Failed decoding multiaddr")]
    Multiaddr(#[from] multiaddr::Error),
    #[error("Failed decoding public key")]
    PublicKey(#[from] identity::DecodingError),
}
