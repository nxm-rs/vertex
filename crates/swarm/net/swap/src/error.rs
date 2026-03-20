//! Error types for swap protocol.

vertex_net_codec::protocol_error! {
    /// SWAP protocol errors.
    pub enum SwapError {
        /// Missing required header.
        #[error("missing header: {0}")]
        MissingHeader(&'static str),

        /// Invalid beneficiary address length.
        #[error("invalid beneficiary length: expected 20, got {0}")]
        #[strum(serialize = "invalid_beneficiary")]
        InvalidBeneficiaryLength(usize),

        /// JSON serialization/deserialization error.
        #[error("json error: {0}")]
        #[strum(serialize = "json_error")]
        Json(String),
    }
}

impl From<serde_json::Error> for SwapError {
    fn from(error: serde_json::Error) -> Self {
        SwapError::Json(error.to_string())
    }
}
