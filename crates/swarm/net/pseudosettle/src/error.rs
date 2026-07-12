//! Error types for pseudosettle protocol.

vertex_net_codec::protocol_error! {
    /// Pseudosettle protocol errors.
    pub enum PseudosettleError {
        /// Malformed payment amount bytes.
        #[error("invalid payment amount: {0}")]
        #[strum(serialize = "invalid_amount")]
        InvalidAmount(#[from] vertex_net_codec::U256DecodeError),
    }
}
