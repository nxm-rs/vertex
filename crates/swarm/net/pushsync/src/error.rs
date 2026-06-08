//! Error types for pushsync protocol.

vertex_net_codec::protocol_error! {
    /// Pushsync protocol errors.
    pub enum PushsyncError {
        /// Invalid chunk address length.
        #[error("invalid chunk address length: expected 32, got {0}")]
        InvalidAddressLength(usize),

        /// Invalid chunk address encoding.
        #[error("invalid chunk address: {0}")]
        InvalidAddress(String),

        /// Malformed postage stamp in the delivery.
        #[error("invalid stamp: {0}")]
        InvalidStamp(String),

        /// Malformed receipt signature.
        #[error("invalid signature: {0}")]
        InvalidSignature(String),

        /// Receipt nonce was not exactly 32 bytes.
        #[error("invalid nonce length: expected 32, got {0}")]
        InvalidNonceLength(usize),

        /// Storage radius is outside the valid bin range.
        #[error("invalid storage radius: {0}")]
        InvalidStorageRadius(u32),
    }
}
