//! Error types for retrieval protocol.

vertex_net_codec::protocol_error! {
    /// Retrieval protocol errors.
    pub enum RetrievalError {
        /// Invalid chunk address length.
        #[error("invalid chunk address length: expected 32, got {0}")]
        InvalidAddressLength(usize),

        /// Invalid chunk address encoding.
        #[error("invalid chunk address: {0}")]
        InvalidAddress(#[from] nectar_primitives::PrimitivesError),

        /// Malformed postage stamp in the delivery.
        #[error("invalid stamp: {0}")]
        InvalidStamp(#[from] nectar_postage::StampError),

        /// Chunk bytes did not match the requested address.
        #[error("invalid chunk: {0}")]
        InvalidChunk(#[from] vertex_swarm_primitives::ReconstructError),
    }
}
