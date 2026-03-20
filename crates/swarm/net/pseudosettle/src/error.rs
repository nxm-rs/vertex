//! Error types for pseudosettle protocol.

vertex_net_codec::protocol_error! {
    /// Pseudosettle protocol errors.
    pub enum PseudosettleError {
        /// Invalid timestamp in acknowledgment.
        #[error("invalid timestamp: {0}")]
        #[strum(serialize = "invalid_timestamp")]
        InvalidTimestamp(String),
    }
}
