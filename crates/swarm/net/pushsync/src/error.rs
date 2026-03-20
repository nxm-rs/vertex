//! Error types for pushsync protocol.

vertex_net_codec::protocol_error! {
    /// Pushsync protocol errors.
    pub enum PushsyncError {
        /// Invalid chunk address length.
        #[error("invalid chunk address length: expected 32, got {0}")]
        #[strum(serialize = "invalid_address_length")]
        InvalidAddressLength(usize),

        /// Invalid chunk address encoding.
        #[error("invalid chunk address: {0}")]
        #[strum(serialize = "invalid_address")]
        InvalidAddress(String),

        /// Storage radius exceeds maximum (255).
        #[error("invalid storage radius: {0} exceeds u8 range")]
        #[strum(serialize = "invalid_storage_radius")]
        InvalidStorageRadius(u32),
    }
}
