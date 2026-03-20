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

        /// Storage radius exceeds maximum (255).
        #[error("invalid storage radius: {0} exceeds u8 range")]
        InvalidStorageRadius(u32),
    }
}
