use libp2p::multiaddr;
use thiserror::Error;
use vertex_network_primitives_traits::NodeAddressError;

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("Network ID mismatch")]
    NetworkIDMismatch,

    #[error("Missing field: {0}")]
    MissingField(&'static str),

    #[error("Maximum {0} field length exceeded limit {1}, received {2}")]
    FieldLengthLimitExceeded(&'static str, usize, usize),

    #[error("Invalid data conversion: {0}")]
    InvalidData(#[from] std::array::TryFromSliceError),

    #[error("Invalid Multiaddr: {0}")]
    InvalidMultiaddr(#[from] multiaddr::Error),

    #[error("Invalid signature: {0}")]
    InvalidSignature(#[from] alloy_primitives::SignatureError),

    #[error("Invalid node address: {0}")]
    InvalidNodeAddress(#[from] NodeAddressError),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<quick_protobuf_codec::Error> for CodecError {
    fn from(error: quick_protobuf_codec::Error) -> Self {
        CodecError::Protocol(error.to_string())
    }
}
