//! Error types for retrieval protocol.

vertex_net_codec::protocol_error! {
    /// Retrieval protocol errors.
    pub enum RetrievalError {
        /// Invalid chunk address length.
        #[error("invalid chunk address length: expected 32, got {0}")]
        InvalidAddressLength(usize),

        /// Invalid chunk address encoding.
        #[error("invalid chunk address: {0}")]
        InvalidAddress(String),
    }
}
