use crate::codec;

#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error("Picker rejection")]
    PickerRejection,
    #[error("Timeout")]
    Timeout,
    #[error("Codec error: {0}")]
    Codec(#[from] codec::CodecError),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("Stream error: {0}")]
    Stream(#[from] std::io::Error),
    #[error("Connection closed")]
    ConnectionClosed,
    #[error("Missing data")]
    MissingData,
}
