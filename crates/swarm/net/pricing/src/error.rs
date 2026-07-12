//! Error types for pricing protocol.

vertex_net_codec::protocol_error! {
    /// Pricing protocol errors.
    pub enum PricingError {
        /// Malformed payment threshold bytes.
        #[error("invalid payment threshold: {0}")]
        #[strum(serialize = "invalid_threshold")]
        InvalidThreshold(#[from] vertex_net_codec::U256DecodeError),
    }
}
