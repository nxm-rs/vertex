use std::array::TryFromSliceError;

#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error("Network ID incompatible")]
    NetworkIDIncompatible,
    #[error("Invalid ACK")]
    InvalidAck,
    #[error("Invalid SYN")]
    InvalidSyn,
    #[error("Welcome message too long")]
    WelcomeMessageTooLong,
    #[error("Picker rejection")]
    PickerRejection,
    #[error("Timeout")]
    Timeout,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("Quick protobuf error: {0}")]
    QuickProtobuf(#[from] quick_protobuf::Error),
    #[error("Failed to parse observed underlay: {0}")]
    ParseUnderlay(#[from] libp2p::multiaddr::Error),
    #[error("Failed to parse nonce: {0}")]
    ParseSlice(#[from] TryFromSliceError),
    #[error("Field {0} is required")]
    MissingField(&'static str),
    #[error("Failed to parse signature: {0}")]
    SignatureError(#[from] alloy::primitives::SignatureError),
    #[error("Alloy signer error: {0}")]
    SignerError(#[from] alloy::signers::Error),
    #[error("NodeAddress conversion error: {0}")]
    NodeAddressConversion(#[from] vertex_network_primitives_traits::NodeAddressError),
}
